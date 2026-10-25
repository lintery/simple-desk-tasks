//! Meetings from the user's calendars, read-only, for the calendar widget:
//! CalDAV accounts with an app password (Mail.ru, Яндекс, iCloud, any server)
//! and subscriptions by .ics link (Google's secret address, Outlook's
//! published calendar, any). Passwords and secret links live in the Windows
//! Credential Manager; the accounts and the chosen calendars in the settings.
//! Synced every few minutes, with a reminder shortly before each meeting.

use crate::{
    desktop::{finish, APP_ID},
    errors,
    ical::{self, Event},
    settings::{self, CalendarAccount, CalendarRef, LEGACY_ACCOUNT},
};
use serde::{Deserialize, Serialize};
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
const SYNC_EVERY: Duration = Duration::from_secs(5 * 60);
/// A request that takes longer is given up on.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// A reminder this long before a meeting.
const REMIND_MS: i64 = 5 * 60_000;
const NS: &str = "xmlns:d='DAV:' xmlns:c='urn:ietf:params:xml:ns:caldav' xmlns:a='http://apple.com/ns/ical/'";
/// Where the secret of an account is kept: this, then its id.
const CREDENTIAL_PREFIX: &str = "SimpleDeskTasks/calendar/";
/// Where the one Mail.ru password was kept before there were several accounts.
const LEGACY_CREDENTIAL: &str = "SimpleDeskTasks/calendar.mail.ru";
/// The calendar of a link account (it holds just one).
const LINK_CALENDAR: &str = "link";
/// Colours for calendars that don't bring their own.
const PALETTE: [&str; 6] = ["#8e9fff", "#5cc8a8", "#f0a35e", "#e57aa8", "#6cb6ff", "#c792ea"];

// ---------- providers ----------

enum Kind {
    /// CalDAV: the server (empty: the user gives it), where discovery starts,
    /// and the sites the password may be sent to (a host or its subdomains;
    /// empty: the server's own host).
    CalDav { server: &'static str, starts: &'static [&'static str], hosts: &'static [&'static str] },
    /// An .ics link.
    Link,
}

struct Provider {
    id: &'static str,
    name: &'static str,
    kind: Kind,
}

const PROVIDERS: [Provider; 7] = [
    Provider {
        id: "mailru",
        name: "Mail.ru",
        // /.well-known/caldav redirects to /principals/; the site's root is
        // the web app, which says 400 to PROPFIND.
        kind: Kind::CalDav { server: settings::MAILRU_SERVER, starts: &["/principals/", "/.well-known/caldav"], hosts: &["calendar.mail.ru"] },
    },
    Provider {
        id: "yandex",
        name: "Яндекс",
        kind: Kind::CalDav { server: "https://caldav.yandex.ru", starts: &["/.well-known/caldav", "/"], hosts: &["caldav.yandex.ru"] },
    },
    Provider {
        id: "icloud",
        name: "iCloud",
        // The account's calendars live on a pNN-caldav.icloud.com host.
        kind: Kind::CalDav { server: "https://caldav.icloud.com", starts: &["/.well-known/caldav", "/"], hosts: &["icloud.com"] },
    },
    Provider { id: "caldav", name: "CalDAV", kind: Kind::CalDav { server: "", starts: &["", "/.well-known/caldav"], hosts: &[] } },
    Provider { id: "google", name: "Google", kind: Kind::Link },
    Provider { id: "outlook", name: "Outlook", kind: Kind::Link },
    Provider { id: "ics", name: "Календарь по ссылке", kind: Kind::Link },
];

fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// Where requests for an account may go: its CalDAV server's site (with the
/// password) — redirects elsewhere are not followed.
struct Site {
    name: &'static str,
    host: String,
    hosts: &'static [&'static str],
}

impl Site {
    fn of(p: &Provider, server: &str) -> Site {
        let hosts = match p.kind {
            Kind::CalDav { hosts, .. } => hosts,
            Kind::Link => &[],
        };
        Site { name: p.name, host: host_of(server).unwrap_or_default(), hosts }
    }

    /// `url` is https and on this site (a listed host or its subdomain; with
    /// no list, the server's own host).
    fn allows(&self, url: &str) -> bool {
        let Ok(uri) = Uri::CreateUri(&HSTRING::from(url)) else { return false };
        let (Ok(scheme), Ok(host)) = (uri.SchemeName(), uri.Host()) else { return false };
        let host = host.to_string().to_ascii_lowercase();
        if !scheme.to_string().eq_ignore_ascii_case("https") || !uri.UserName().is_ok_and(|u| u.is_empty()) {
            return false;
        }
        if self.hosts.is_empty() {
            return !self.host.is_empty() && host == self.host;
        }
        self.hosts.iter().any(|h| host == *h || host.ends_with(&format!(".{h}")))
    }
}

fn host_of(url: &str) -> Option<String> {
    let uri = Uri::CreateUri(&HSTRING::from(url.trim())).ok()?;
    Some(uri.Host().ok()?.to_string().to_ascii_lowercase()).filter(|h| !h.is_empty())
}

// ---------- state ----------

/// A meeting in one of the user's calendars.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Meeting {
    #[serde(flatten)]
    pub event: Event,
    pub calendar: String,
    pub color: String,
}

/// How an account is doing, for the settings window.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountState {
    pub id: String,
    pub error: Option<String>,
    /// When all its calendars were last fetched (unix ms), 0 = never.
    pub updated: i64,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub configured: bool,
    pub meetings: Vec<Meeting>,
    /// When every calendar was last fetched (unix ms), 0 = never.
    pub updated: i64,
    /// What's wrong, for the widget: the first account in trouble.
    pub error: Option<String>,
    pub syncing: bool,
    pub accounts: Vec<AccountState>,
    /// Meetings by (account, calendar): a calendar that fails keeps its last ones.
    #[serde(skip)]
    by_calendar: HashMap<(String, String), Vec<Meeting>>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static WAKE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
/// Bumped when accounts change: a sync begun before then publishes nothing.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn state() -> State {
    STATE.lock().unwrap().clone().unwrap_or_default()
}

/// Changes the state and shows it, unless the accounts changed since
/// `generation` (then the change is stale).
fn publish(app: &AppHandle, generation: u64, f: impl FnOnce(&mut State)) {
    let mut guard = STATE.lock().unwrap();
    if GENERATION.load(Ordering::SeqCst) != generation {
        return;
    }
    let s = guard.get_or_insert_with(State::default);
    f(s);
    // The widget and the settings window both show it; sent under the lock so
    // two publishes can't arrive swapped.
    let _ = app.emit("calendar", &*s);
}

/// Accounts changed: earlier syncs are void.
fn new_generation() -> u64 {
    GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

/// Syncs soon (settings changed, connected, or asked for).
pub fn refresh() {
    let (flag, cv) = &WAKE;
    *flag.lock().unwrap() = true;
    cv.notify_all();
}

// ---------- secrets ----------

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn target(id: &str) -> String {
    format!("{CREDENTIAL_PREFIX}{id}")
}

fn write_secret(target: &str, user: &str, secret: &str) -> Result<(), String> {
    let (target, user) = (wide(target), wide(user));
    let mut blob = secret.as_bytes().to_vec();
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
    unsafe { CredWriteW(&cred, 0) }.map_err(|_| "Не получилось сохранить пароль в Диспетчере учётных данных Windows".to_string())
}

fn read_secret(target: &str) -> Option<String> {
    let target = wide(target);
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

fn delete_secret(target: &str) {
    let target = wide(target);
    // SAFETY: plain call with a live string.
    let _ = unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) };
}

/// An account's password or secret link. The Mail.ru account from before
/// there were several finds its password where it was kept then, and moves it.
fn load_secret(account: &CalendarAccount) -> Option<String> {
    if let Some(secret) = read_secret(&target(&account.id)) {
        return Some(secret);
    }
    if account.id != LEGACY_ACCOUNT {
        return None;
    }
    let secret = read_secret(LEGACY_CREDENTIAL)?;
    if write_secret(&target(&account.id), &account.label, &secret).is_ok() {
        delete_secret(LEGACY_CREDENTIAL);
    }
    Some(secret)
}

// ---------- CalDAV ----------

struct Reply {
    status: u16,
    body: String,
    /// Where a redirect points.
    location: Option<String>,
    /// The URL that answered (after redirects): hrefs in the body are relative to it.
    url: String,
}

/// Redirects followed by hand: the system won't follow one for PROPFIND (it
/// wants a user's say-so) and fails the request; Mail.ru's and others'
/// /.well-known/caldav redirect.
const MAX_REDIRECTS: usize = 5;

fn dav(site: &Site, method: &str, url: &str, depth: &str, body: &str, auth: &str) -> Result<Reply, String> {
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let reply = send(method, &url, depth, body, auth).map_err(|e| errors::net(&e, &site.host))?;
        match (reply.status, &reply.location) {
            // The password goes along, so only within the account's own site.
            (301 | 302 | 303 | 307 | 308, Some(to)) if site.allows(&absolute(&url, to)) => url = absolute(&url, to),
            (200..=299, _) => return Ok(Reply { url, ..reply }),
            (401 | 403, _) => return Err(denied(site)),
            (404, _) => return Err(NOT_FOUND.into()),
            (s, _) => return Err(errors::http(s, &site.host)),
        }
    }
    Err(format!("{} отвечает перенаправлениями по кругу", site.host))
}

/// dav()'s answer to a 404: the caller says what wasn't found.
const NOT_FOUND: &str = "not found";

/// dav()'s answer to a 401/403.
fn denied(site: &Site) -> String {
    format!("{} не принял логин или пароль приложения", site.name)
}

/// A URL the server pointed to, if the password may go there. A server behind
/// a proxy may name its own pages with http://: they're asked over https.
fn within(site: &Site, url: String) -> Option<String> {
    if site.allows(&url) {
        return Some(url);
    }
    let https = match url.get(..7) {
        Some(s) if s.eq_ignore_ascii_case("http://") => format!("https://{}", &url[7..]),
        _ => return None,
    };
    site.allows(&https).then_some(https)
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
    headers.TryAppendWithoutValidation(&HSTRING::from("User-Agent"), &HSTRING::from("SimpleDeskWidgets"))?;
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

fn auth(login: &str, password: &str) -> String {
    format!("Basic {}", crate::media::base64(format!("{login}:{password}").as_bytes()))
}

/// `href` relative to `base`.
fn absolute(base: &str, href: &str) -> String {
    Uri::CreateUri(&HSTRING::from(base))
        .and_then(|b| b.CombineUri(&HSTRING::from(href.trim())))
        .and_then(|u| u.AbsoluteUri())
        .map(|s| s.to_string())
        .unwrap_or_else(|_| href.trim().to_string())
}

fn xml(site: &Site, body: &str) -> Result<XmlDocument, String> {
    let unclear = || format!("{} ответил непонятно — это точно адрес CalDAV?", site.name);
    let doc = XmlDocument::new().map_err(|_| unclear())?;
    doc.LoadXml(&HSTRING::from(body)).map_err(|_| unclear())?;
    Ok(doc)
}

/// The d:/c:/a: prefixes for XPath queries.
fn namespaces() -> Option<IInspectable> {
    PropertyValue::CreateString(&HSTRING::from(NS)).ok()
}

/// Text of the first node matching `path`.
fn first_text(doc: &XmlDocument, path: &str) -> Option<String> {
    let node = doc.SelectSingleNodeNS(&HSTRING::from(path), &namespaces()?).ok()?;
    node.InnerText().ok().map(|s| s.to_string().trim().to_string()).filter(|s| !s.is_empty())
}

/// The answer, and the URL that gave it (hrefs in it are relative to that).
fn prop_find(site: &Site, url: &str, depth: &str, props: &str, auth: &str) -> Result<(XmlDocument, String), String> {
    let body = format!("<?xml version='1.0' encoding='utf-8'?><d:propfind {NS}><d:prop>{props}</d:prop></d:propfind>");
    let reply = dav(site, "PROPFIND", url, depth, &body, auth)?;
    Ok((xml(site, &reply.body)?, reply.url))
}

/// `#RRGGBB[AA]` → `#rrggbb`.
fn color(raw: &str) -> String {
    let hex = raw.trim().trim_start_matches('#');
    match hex.get(..6) {
        Some(rgb) if rgb.bytes().all(|b| b.is_ascii_hexdigit()) => format!("#{}", rgb.to_ascii_lowercase()),
        _ => String::new(),
    }
}

/// A CalDAV account's calendars that hold events.
fn discover(p: &Provider, server: &str, login: &str, password: &str) -> Result<Vec<CalendarRef>, String> {
    let Kind::CalDav { starts, .. } = p.kind else { return Err("Это не CalDAV".into()) };
    let site = Site::of(p, server);
    let auth = auth(login, password);
    let server = server.trim_end_matches('/');
    let mut principal = None;
    let mut answered = None;
    let mut last_err = String::new();
    for start in starts {
        let url = if start.is_empty() { server.to_string() } else { format!("{}{start}", origin(server)) };
        match prop_find(&site, &url, "0", "<d:current-user-principal/>", &auth) {
            Ok((doc, at)) => {
                principal = first_text(&doc, "//d:current-user-principal/d:href").map(|href| absolute(&at, &href));
                answered = Some(at);
                break; // the other ways in lead to the same place
            }
            Err(e) if e == denied(&site) => return Err(e),
            Err(e) => last_err = e,
        }
    }
    let nothing = || format!("Не нашёл календарей на {} — проверьте адрес", site.host);
    // No current-user-principal: the collection that answered may be the principal itself.
    let principal = principal.or(answered).ok_or_else(|| if last_err.is_empty() || last_err == NOT_FOUND { nothing() } else { last_err })?;
    let principal = within(&site, principal).ok_or_else(nothing)?;
    let (doc, at) = prop_find(&site, &principal, "0", "<c:calendar-home-set/>", &auth).map_err(|e| if e == NOT_FOUND { nothing() } else { e })?;
    let home_href = first_text(&doc, "//c:calendar-home-set/d:href").ok_or_else(nothing)?;
    let home = within(&site, absolute(&at, &home_href)).ok_or_else(nothing)?;
    let (doc, home) = prop_find(&site, &home, "1", "<d:resourcetype/><d:displayname/><a:calendar-color/><c:supported-calendar-component-set/>", &auth)
        .map_err(|e| if e == NOT_FOUND { nothing() } else { e })?;
    let ns = namespaces().ok_or_else(nothing)?;
    let responses = doc.SelectNodesNS(&HSTRING::from("//d:response"), &ns).map_err(|_| nothing())?;
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
        let Some(url) = within(&site, absolute(&home, &href)) else { continue };
        if !is_calendar || !events {
            continue;
        }
        out.push(CalendarRef {
            url,
            name: pick(".//d:displayname").filter(|n| !n.is_empty()).unwrap_or_else(|| "Календарь".into()),
            color: pick(".//a:calendar-color").map(|c| color(&c)).unwrap_or_default(),
            enabled: true,
        });
    }
    out.retain(|c| !settings::hidden_calendar(&c.name));
    if out.is_empty() {
        return Err("В аккаунте нет календарей со встречами".into());
    }
    Ok(out)
}

/// `https://host[:port]` of a URL.
fn origin(url: &str) -> String {
    Uri::CreateUri(&HSTRING::from(url))
        .and_then(|u| Ok(format!("{}://{}", u.SchemeName()?, u.Host()?) + &port_suffix(&u)))
        .unwrap_or_else(|_| url.trim_end_matches('/').to_string())
}

fn port_suffix(u: &Uri) -> String {
    match u.Port() {
        Ok(port) if port != 443 && port != 80 && port > 0 => format!(":{port}"),
        _ => String::new(),
    }
}

fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%Y%m%dT%H%M%SZ").to_string()).unwrap_or_default()
}

/// Meetings of one CalDAV calendar between `from` and `to` (unix ms).
fn fetch(site: &Site, cal: &CalendarRef, auth: &str, from: i64, to: i64) -> Result<Vec<Meeting>, String> {
    let (s, e) = (stamp(from), stamp(to));
    let body = format!(
        "<?xml version='1.0' encoding='utf-8'?><c:calendar-query {NS}><d:prop><c:calendar-data>\
         <c:expand start='{s}' end='{e}'/></c:calendar-data></d:prop><c:filter><c:comp-filter name='VCALENDAR'>\
         <c:comp-filter name='VEVENT'><c:time-range start='{s}' end='{e}'/></c:comp-filter></c:comp-filter></c:filter>\
         </c:calendar-query>"
    );
    let reply = dav(site, "REPORT", &cal.url, "1", &body, auth).map_err(|e| {
        if e == NOT_FOUND {
            format!("Календаря «{}» больше нет — подключите аккаунт заново", cal.name)
        } else {
            e
        }
    })?;
    let doc = xml(site, &reply.body)?;
    let ns = namespaces().ok_or_else(|| format!("{} ответил непонятно", site.name))?;
    let nodes = doc.SelectNodesNS(&HSTRING::from("//c:calendar-data"), &ns).map_err(|_| format!("{} ответил непонятно", site.name))?;
    let mut out = Vec::new();
    for i in 0..nodes.Length().unwrap_or(0) {
        let Some(text) = nodes.Item(i).ok().and_then(|n| n.InnerText().ok()) else { continue };
        for event in ical::parse(&text.to_string(), from, to) {
            out.push(Meeting { event, calendar: cal.name.clone(), color: cal.color.clone() });
        }
    }
    Ok(out)
}

// ---------- links (.ics) ----------

/// A link as given: webcal:// is https://; only web links are taken.
fn link_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let url = match raw.get(..9) {
        Some(s) if s.eq_ignore_ascii_case("webcal://") => format!("https://{}", &raw[9..]),
        _ => raw.to_string(),
    };
    // The link is the secret: never over plain http.
    if !url.to_ascii_lowercase().starts_with("https://") || host_of(&url).is_none() {
        return Err("Нужна ссылка на календарь: она начинается с https:// или webcal://".into());
    }
    Ok(url)
}

/// The calendar a link holds: its text, checked to be one.
fn fetch_link(url: &str) -> Result<String, String> {
    let host = host_of(url).unwrap_or_default();
    let bytes = crate::net::get(url, HTTP_TIMEOUT, &host)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if !text.contains("BEGIN:VCALENDAR") {
        return Err(format!("По ссылке с {host} не календарь — нужна ссылка на файл .ics"));
    }
    Ok(text)
}

/// The calendar's own name (X-WR-CALNAME), if it says.
fn link_name(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.trim_end_matches('\r').strip_prefix("X-WR-CALNAME:"))
        .map(|n| n.replace("\\,", ",").trim().to_string())
        .filter(|n| !n.is_empty())
}

// ---------- sync ----------

/// The window the widget shows: from the Monday before this month's first day
/// to six weeks ahead.
fn window() -> (i64, i64) {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let first = today.with_day(1).unwrap_or(today);
    let back = (today - first).num_days() + first.weekday().num_days_from_monday() as i64;
    (ical::day_start(-back), ical::day_start(42))
}

/// What one account's sync brought: per calendar, its meetings or why not.
fn sync_account(account: &CalendarAccount, from: i64, to: i64) -> Result<Vec<(String, Result<Vec<Meeting>, String>)>, String> {
    let p = provider(&account.provider).ok_or("Этот календарь не поддерживается — удалите его и подключите заново")?;
    let secret = load_secret(account).ok_or("Пароль не найден — подключите аккаунт заново")?;
    let enabled: Vec<&CalendarRef> = account.calendars.iter().filter(|c| c.enabled).collect();
    match p.kind {
        Kind::CalDav { .. } => {
            let site = Site::of(p, &account.server);
            let auth = auth(&account.label, &secret);
            Ok(enabled.iter().map(|cal| (cal.url.clone(), fetch(&site, cal, &auth, from, to))).collect())
        }
        Kind::Link => {
            let Some(cal) = enabled.first() else { return Ok(Vec::new()) };
            let meetings = fetch_link(&secret).map(|text| {
                ical::parse(&text, from, to)
                    .into_iter()
                    .map(|event| Meeting { event, calendar: cal.name.clone(), color: cal.color.clone() })
                    .collect()
            });
            Ok(vec![(cal.url.clone(), meetings)])
        }
    }
}

fn sync(app: &AppHandle) {
    let generation = GENERATION.load(Ordering::SeqCst);
    let accounts = settings::get().calendar_accounts;
    if accounts.is_empty() {
        publish(app, generation, |st| *st = State::default());
        return;
    }
    publish(app, generation, |st| {
        st.configured = true;
        st.syncing = true;
    });
    let (from, to) = window();
    let results: Vec<(&CalendarAccount, Result<Vec<(String, Result<Vec<Meeting>, String>)>, String>)> =
        accounts.iter().map(|a| (a, sync_account(a, from, to))).collect();
    let now = crate::store::now_ms();
    publish(app, generation, |st| {
        st.syncing = false;
        // Only calendars still there and switched on keep their meetings.
        st.by_calendar.retain(|(id, url), _| {
            accounts.iter().any(|a| &a.id == id && a.calendars.iter().any(|c| &c.url == url && c.enabled))
        });
        let before = std::mem::take(&mut st.accounts);
        for (account, result) in results {
            let mut errors: Vec<String> = Vec::new();
            match result {
                Ok(calendars) => {
                    for (url, fetched) in calendars {
                        match fetched {
                            Ok(meetings) => {
                                st.by_calendar.insert((account.id.clone(), url), meetings);
                            }
                            // A calendar that failed keeps what it had (shown with its time).
                            Err(e) if !errors.contains(&e) => errors.push(e),
                            Err(_) => {}
                        }
                    }
                }
                Err(e) => errors.push(e),
            }
            let updated = if errors.is_empty() { now } else { before.iter().find(|b| b.id == account.id).map_or(0, |b| b.updated) };
            st.accounts.push(AccountState { id: account.id.clone(), error: (!errors.is_empty()).then(|| errors.join("; ")), updated });
        }
        sum_up(st, &accounts);
    });
}

/// The widget's view of all accounts: the first error, the oldest update of
/// those that have loaded, every meeting in order.
fn sum_up(st: &mut State, accounts: &[CalendarAccount]) {
    st.configured = !accounts.is_empty();
    st.error = st.accounts.iter().find_map(|s| {
        let e = s.error.as_ref()?;
        let a = accounts.iter().find(|a| a.id == s.id)?;
        Some(if accounts.len() > 1 { format!("{}: {e}", account_title(a)) } else { e.clone() })
    });
    st.updated = st.accounts.iter().map(|a| a.updated).filter(|&u| u > 0).min().unwrap_or(0);
    let mut meetings: Vec<Meeting> = st.by_calendar.values().flatten().cloned().collect();
    meetings.sort_by(|a, b| (a.event.start, a.event.end, &a.event.id).cmp(&(b.event.start, b.event.end, &b.event.id)));
    st.meetings = meetings;
}

/// How an account is named to the user: «Яндекс · me@yandex.ru».
fn account_title(a: &CalendarAccount) -> String {
    let name = provider(&a.provider).map_or("Календарь", |p| p.name);
    if a.label.is_empty() || a.label == name { name.to_string() } else { format!("{name} · {}", a.label) }
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

// ---------- commands ----------

#[tauri::command]
pub fn calendar_state() -> State {
    let mut s = state();
    s.configured = !settings::get().calendar_accounts.is_empty();
    s
}

/// What the settings window sends to connect (or reconnect) an account.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connect {
    provider: String,
    /// CalDAV: the login (usually the email).
    #[serde(default)]
    login: String,
    /// CalDAV: the app password; a link account: the link.
    secret: String,
    /// The user's own CalDAV server.
    #[serde(default)]
    server: String,
    /// Reconnecting this account (a new password or link), not adding one.
    #[serde(default)]
    account: Option<String>,
}

/// Checks the account, saves its secret and the calendars found.
#[tauri::command]
pub async fn calendar_connect(app: AppHandle, request: Connect) -> Result<CalendarAccount, String> {
    tauri::async_runtime::spawn_blocking(move || {
        // SAFETY: as in `spawn`.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        connect(&app, request)
    })
    .await
    .map_err(|_| "Не получилось подключить — попробуйте ещё раз".to_string())?
}

fn connect(app: &AppHandle, r: Connect) -> Result<CalendarAccount, String> {
    let p = provider(&r.provider).ok_or("Такой календарь не поддерживается")?;
    let secret = r.secret.trim().to_string();
    let existing = settings::get().calendar_accounts;
    let reconnecting = r.account.as_ref().and_then(|id| existing.iter().find(|a| &a.id == id));
    if r.account.is_some() && reconnecting.is_none() {
        return Err(GONE.into());
    }
    let colours_used = existing.iter().map(|a| a.calendars.len()).sum::<usize>();
    let (label, server, mut calendars, secret) = match p.kind {
        Kind::CalDav { server, .. } => {
            let login = r.login.trim().to_string();
            if login.is_empty() || secret.is_empty() {
                return Err("Нужны логин и пароль приложения".into());
            }
            let server = if server.is_empty() {
                let given = r.server.trim().trim_end_matches('/').to_string();
                if !given.to_ascii_lowercase().starts_with("https://") || host_of(&given).is_none() {
                    return Err("Нужен адрес сервера CalDAV, начинается с https://".into());
                }
                given
            } else {
                server.to_string()
            };
            let calendars = discover(p, &server, &login, &secret)?;
            (login, server, calendars, secret)
        }
        Kind::Link => {
            if secret.is_empty() {
                return Err("Вставьте ссылку на календарь".into());
            }
            let url = link_url(&secret)?;
            let text = fetch_link(&url)?;
            let name = link_name(&text).unwrap_or_else(|| p.name.to_string());
            let cal = CalendarRef { url: LINK_CALENDAR.into(), name: name.clone(), color: String::new(), enabled: true };
            (name, host_of(&url).unwrap_or_default(), vec![cal], url)
        }
    };
    // Calendars seen before keep their colour; new ones without their own take
    // the next of the palette.
    let mut fresh = 0;
    for cal in calendars.iter_mut().filter(|c| c.color.is_empty()) {
        match reconnecting.and_then(|a| a.calendars.iter().find(|o| o.url == cal.url)) {
            Some(seen) if !seen.color.is_empty() => cal.color = seen.color.clone(),
            _ => {
                cal.color = PALETTE[(colours_used + fresh) % PALETTE.len()].to_string();
                fresh += 1;
            }
        }
    }
    // A reconnect keeps the account's id; a new one gets its own.
    let id = reconnecting.map_or_else(|| format!("{}-{:x}", p.id, crate::store::now_ms()), |a| a.id.clone());
    let account = CalendarAccount { id: id.clone(), provider: p.id.into(), label: label.clone(), server, calendars };
    let saved = account.clone();
    let reconnect = reconnecting.is_some();
    let mut gone = false;
    // Under the settings lock: an account disconnected meanwhile stays gone.
    settings::update(app, |s| {
        let mut next = saved;
        if let Some(old) = s.calendar_accounts.iter_mut().find(|a| a.id == next.id) {
            // Keep the user's on/off choice for calendars seen before.
            for cal in &mut next.calendars {
                if let Some(seen) = old.calendars.iter().find(|o| o.url == cal.url) {
                    cal.enabled = seen.enabled;
                }
            }
            *old = next;
        } else if reconnect {
            gone = true;
        } else {
            s.calendar_accounts.push(next);
        }
    })?;
    if gone {
        return Err(GONE.into());
    }
    if let Err(e) = write_secret(&target(&id), &label, &secret) {
        // Without its secret the account is no use: take it back out.
        if !reconnect {
            let _ = settings::update(app, |s| s.calendar_accounts.retain(|a| a.id != id));
        }
        return Err(e);
    }
    // Disconnected while the secret was being written: don't leave it behind.
    if !settings::get().calendar_accounts.iter().any(|a| a.id == id) {
        delete_secret(&target(&id));
        return Err(GONE.into());
    }
    if id == LEGACY_ACCOUNT {
        delete_secret(LEGACY_CREDENTIAL);
    }
    new_generation();
    refresh();
    Ok(account)
}

/// A reconnect of an account disconnected meanwhile.
const GONE: &str = "Этот календарь уже отключён — подключите его заново";

#[tauri::command]
pub fn calendar_disconnect(app: AppHandle, account: String) -> Result<(), String> {
    // Settings first: if they can't be saved, the account keeps its secret.
    let accounts = settings::update(&app, |s| s.calendar_accounts.retain(|a| a.id != account))?.calendar_accounts;
    delete_secret(&target(&account));
    if account == LEGACY_ACCOUNT {
        delete_secret(LEGACY_CREDENTIAL);
    }
    // A sync still running must not bring it back; the others sync again.
    publish(&app, new_generation(), |st| {
        st.by_calendar.retain(|(id, _), _| *id != account);
        st.accounts.retain(|a| a.id != account);
        sum_up(st, &accounts);
    });
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
    fn passwords_go_only_to_the_account_s_site() {
        let mailru = Site::of(provider("mailru").unwrap(), settings::MAILRU_SERVER);
        assert!(mailru.allows("https://calendar.mail.ru/principals/"));
        assert!(!mailru.allows("https://calendar.mail.ru.evil.example/x"));
        assert!(!mailru.allows("http://calendar.mail.ru/principals/"));
        assert!(!mailru.allows("https://calendar.mail.ru@evil.example/"));
        let icloud = Site::of(provider("icloud").unwrap(), "https://caldav.icloud.com");
        assert!(icloud.allows("https://p68-caldav.icloud.com:443/123/calendars/"));
        assert!(!icloud.allows("https://icloud.com.evil.example/"));
        let own = Site::of(provider("caldav").unwrap(), "https://cloud.example.org/remote.php/dav");
        assert!(own.allows("https://cloud.example.org/remote.php/dav/calendars/me/"));
        assert!(!own.allows("https://other.example.org/"));
    }

    #[test]
    fn links_are_web_links() {
        assert_eq!(link_url(" webcal://example.com/a.ics ").unwrap(), "https://example.com/a.ics");
        assert!(link_url("ftp://example.com/a.ics").is_err());
        assert!(link_url("просто текст").is_err());
        assert_eq!(link_name("BEGIN:VCALENDAR\r\nX-WR-CALNAME:Работа\\, команда\r\n"), Some("Работа, команда".into()));
    }

    /// Against the real server (network): both ways in reach CalDAV, which
    /// turns a made-up account down instead of answering 400.
    #[test]
    #[ignore]
    fn caldav_servers_turn_down_a_made_up_account() {
        // SAFETY: as in `spawn`.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        for (id, server) in [("mailru", settings::MAILRU_SERVER), ("yandex", "https://caldav.yandex.ru"), ("icloud", "https://caldav.icloud.com")] {
            let err = discover(provider(id).unwrap(), server, "nobody-sdt-test@example.com", "wrong").unwrap_err();
            assert!(err.contains("не принял"), "{id}: {err}");
        }
        // A link that isn't a calendar says so.
        let err = fetch_link("https://example.com/").unwrap_err();
        assert!(err.contains("не календарь"), "{err}");
        // Google's public holidays: the same kind of link as a secret address.
        let url = link_url("https://calendar.google.com/calendar/ical/ru.russian%23holiday%40group.v.calendar.google.com/public/basic.ics").unwrap();
        let text = fetch_link(&url).unwrap();
        assert!(link_name(&text).is_some());
        assert!(!ical::parse(&text, 0, i64::MAX / 2).is_empty());
    }
}
