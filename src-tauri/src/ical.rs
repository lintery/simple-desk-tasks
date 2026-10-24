//! Just enough iCalendar (RFC 5545) to show meetings: VEVENTs with their time
//! zones, all-day events, recurrence (RRULE / EXDATE / moved instances) when
//! the server didn't expand it, and a video-call link if there is one.

use chrono::{DateTime, Datelike, Duration, FixedOffset, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc, Weekday};
use serde::Serialize;
use std::collections::HashMap;
use std::rc::Rc;

/// Sites whose links are calls one joins: the host (or a subdomain of it,
/// like a company's team.ktalk.ru or us02web.zoom.us) and a path prefix.
const CALL_HOSTS: [(&str, &str); 12] = [
    ("ktalk.ru", ""), // Контур.Толк
    ("zoom.us", ""),
    ("telemost.yandex.ru", ""),
    ("telemost.yandex.com", ""),
    ("telemost.360.yandex.ru", ""),
    ("meet.google.com", ""),
    ("teams.microsoft.com", ""),
    ("teams.live.com", ""),
    ("calls.vk.com", ""),
    ("vk.com", "/call"),
    ("salutejazz.ru", ""),
    ("jazz.sber.ru", ""),
];
/// A runaway rule stops here.
const MAX_STEPS: usize = 5000;
const MAX_INTERVAL: i64 = 1000;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// UID plus the instance's original start: unique per occurrence.
    pub id: String,
    pub title: String,
    /// Unix ms.
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub location: String,
    /// A video-call link: from URL, CONFERENCE or a video field, LOCATION,
    /// DESCRIPTION, or else any property.
    pub link: Option<String>,
}

struct Prop {
    name: String,
    params: HashMap<String, String>,
    value: String,
}

/// Property lines of one component, unfolded.
fn props(lines: &[String]) -> Vec<Prop> {
    lines
        .iter()
        .filter_map(|line| {
            // NAME;PARAM=V;PARAM="V:x":VALUE — the first ':' outside quotes ends the head.
            let mut quoted = false;
            let colon = line.char_indices().find(|&(_, c)| {
                if c == '"' {
                    quoted = !quoted;
                }
                c == ':' && !quoted
            })?.0;
            let (head, value) = (&line[..colon], &line[colon + 1..]);
            let mut parts = head.split(';');
            let name = parts.next()?.to_ascii_uppercase();
            let params = parts
                .filter_map(|p| p.split_once('='))
                .map(|(k, v)| (k.to_ascii_uppercase(), v.trim_matches('"').to_string()))
                .collect();
            Some(Prop { name, params, value: value.to_string() })
        })
        .collect()
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// "+0300" / "-0530" → seconds east of UTC.
fn offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let sign = if s.starts_with('-') { -1 } else { 1 };
    let digits = s.trim_start_matches(['+', '-']);
    let h: i32 = digits.get(0..2)?.parse().ok()?;
    let m: i32 = digits.get(2..4)?.parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// One STANDARD or DAYLIGHT part of a VTIMEZONE: from its first onset, each
/// year (if it has a yearly rule, until the rule ends), the offset becomes `to`.
#[derive(Debug, Clone)]
struct TzPart {
    start: NaiveDateTime,
    from: i32,
    to: i32,
    /// Month, n-th weekday (negative from the end) and the weekday of the yearly onset.
    yearly: Option<(u32, i32, Weekday)>,
    /// The last onset of the yearly rule: its UNTIL (UTC) or the year COUNT ends in.
    until: Option<NaiveDateTime>,
    last_year: Option<i32>,
}

impl TzPart {
    /// The onset must not come after the end of the part's rule.
    fn onset_ok(&self, onset: NaiveDateTime) -> bool {
        let utc = onset - Duration::seconds(self.from as i64);
        self.until.is_none_or(|u| utc <= u) && self.last_year.is_none_or(|y| onset.year() <= y)
    }
}

#[derive(Debug, Default)]
struct Tz {
    parts: Vec<TzPart>,
}

impl Tz {
    /// The offset in effect at a local wall time — that of the part whose
    /// onset is the most recent — and whether that wall time exists (it
    /// doesn't in the hour skipped when clocks go forward; then the offset is
    /// the one from before, as RFC 5545 reads such a time).
    fn offset_at(&self, wall: NaiveDateTime) -> Option<(i32, bool)> {
        let mut best: Option<(NaiveDateTime, &TzPart)> = None;
        for p in &self.parts {
            let onset = match p.yearly {
                // This year's onset or last year's — or, for a rule that has
                // ended, its last ones.
                Some((month, n, wd)) => {
                    let ended = p.until.map(|u| u.year()).into_iter().chain(p.last_year).min().unwrap_or(i32::MAX);
                    let end = wall.year().min(ended);
                    [wall.year(), wall.year() - 1, end, end - 1]
                        .into_iter()
                        .filter_map(|y| nth_weekday(y, month, n, wd).map(|d| d.and_time(p.start.time())))
                        .filter(|o| *o <= wall && *o >= p.start && p.onset_ok(*o))
                        .max()
                }
                None => (p.start <= wall).then_some(p.start),
            };
            if let Some(o) = onset {
                if best.is_none_or(|(b, _)| o > b) {
                    best = Some((o, p));
                }
            }
        }
        match best {
            Some((onset, p)) if p.to > p.from && wall < onset + Duration::seconds((p.to - p.from) as i64) => Some((p.from, false)),
            Some((_, p)) => Some((p.to, true)),
            // Before every known onset: the offset the earliest part comes from.
            None => self.parts.iter().min_by_key(|p| p.start).map(|p| (p.from, true)),
        }
    }
}

/// A point in time as written in the file: in UTC, in a zone of the file
/// (VTIMEZONE), or local (floating, or a zone the file doesn't describe).
#[derive(Clone, Debug)]
enum Zone {
    Utc,
    Tz(Rc<Tz>),
    Local,
}

#[derive(Clone, Debug)]
struct When {
    wall: NaiveDateTime,
    zone: Zone,
    date_only: bool,
}

impl When {
    /// Unix ms. A local time skipped by DST is read with the offset from
    /// before the change (RFC 5545 3.3.5): 02:30 on that night is 03:30.
    fn ms(&self) -> Option<i64> {
        Some(match &self.zone {
            Zone::Utc => Utc.from_utc_datetime(&self.wall).timestamp_millis(),
            Zone::Tz(tz) => FixedOffset::east_opt(tz.offset_at(self.wall)?.0)?.from_local_datetime(&self.wall).single()?.timestamp_millis(),
            Zone::Local => match Local.from_local_datetime(&self.wall).earliest() {
                Some(t) => t.timestamp_millis(),
                // wall + 1 h read after the change is the same instant as wall read before it.
                None => Local.from_local_datetime(&(self.wall + Duration::hours(1))).earliest()?.timestamp_millis(),
            },
        })
    }

    /// The wall time exists in its zone (a rule's instance in a DST gap is dropped).
    fn exists(&self) -> bool {
        match &self.zone {
            Zone::Utc => true,
            Zone::Tz(tz) => tz.offset_at(self.wall).is_none_or(|(_, ok)| ok),
            Zone::Local => Local.from_local_datetime(&self.wall).earliest().is_some(),
        }
    }

    fn with_wall(&self, wall: NaiveDateTime) -> When {
        When { wall, ..self.clone() }
    }
}

type Zones = HashMap<String, Rc<Tz>>;

fn parse_when(prop: &Prop, zones: &Zones) -> Option<When> {
    let v = prop.value.trim();
    let date_only = prop.params.get("VALUE").is_some_and(|t| t.eq_ignore_ascii_case("DATE")) || v.len() == 8;
    if date_only {
        let d = NaiveDate::parse_from_str(v.get(0..8)?, "%Y%m%d").ok()?;
        return Some(When { wall: d.and_time(NaiveTime::MIN), zone: Zone::Local, date_only: true });
    }
    let utc = v.ends_with('Z');
    let wall = NaiveDateTime::parse_from_str(v.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok()?;
    let zone = if utc {
        Zone::Utc
    } else {
        match prop.params.get("TZID") {
            Some(tz) => zones.get(tz).map_or(Zone::Local, |z| Zone::Tz(z.clone())),
            None => Zone::Local,
        }
    };
    Some(When { wall, zone, date_only: false })
}

/// "PT1H30M", "P1D", "-PT15M" → duration.
fn parse_duration(v: &str) -> Option<Duration> {
    let (neg, v) = match v.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let v = v.strip_prefix('P')?;
    let (mut total, mut num, mut in_time) = (0i64, String::new(), false);
    for c in v.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += n * match (c, in_time) {
                    ('W', _) => 7 * 86400,
                    ('D', _) => 86400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(Duration::seconds(if neg { -total } else { total }))
}

/// The link goes to a call site (by its host, not just any mention of it).
fn is_call(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) else { return false };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let host = authority.rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("").to_ascii_lowercase();
    CALL_HOSTS.iter().any(|(h, prefix)| {
        let on_host = host == *h || host.strip_suffix(h).is_some_and(|sub| sub.ends_with('.'));
        on_host && path.starts_with(prefix)
    })
}

/// The first call link in the text.
fn call_link(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | ','))
        .map(|w| w.trim_end_matches(['.', ';']).replace("&amp;", "&"))
        .find(|w| is_call(w))
}

#[derive(Debug, Default)]
struct Rule {
    freq: String,
    interval: i64,
    count: Option<usize>,
    /// The last moment instances may start at, and whether it is exclusive
    /// (a date-only UNTIL covers its whole day).
    until: Option<(When, bool)>,
    by_day: Vec<(i32, Weekday)>,
    by_month_day: Vec<i32>,
    by_month: Vec<u32>,
    by_set_pos: Vec<i32>,
}

fn weekday(code: &str) -> Option<Weekday> {
    Some(match code {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// "BYDAY" values: "MO", "-1FR", "2TU".
fn by_day(val: &str) -> Option<Vec<(i32, Weekday)>> {
    val.split(',')
        .map(|d| {
            let d = d.trim();
            let at = d.len().checked_sub(2)?;
            let (n, code) = (d.get(..at)?, d.get(at..)?);
            Some((if n.is_empty() { 0 } else { n.parse().ok()? }, weekday(&code.to_ascii_uppercase())?))
        })
        .collect()
}

fn numbers<T: std::str::FromStr>(val: &str) -> Option<Vec<T>> {
    val.split(',').map(|d| d.trim().parse().ok()).collect()
}

/// A rule we can expand, or `None` (then only DTSTART is shown) for one with
/// parts we don't handle or don't understand.
fn parse_rule(v: &str, zones: &Zones, start: &When) -> Option<Rule> {
    let mut rule = Rule { interval: 1, ..Default::default() };
    for part in v.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (k, val) = part.split_once('=')?;
        match k.to_ascii_uppercase().as_str() {
            "FREQ" => rule.freq = val.to_ascii_uppercase(),
            // A huge interval is nonsense (and would run dates off chrono's range).
            "INTERVAL" => rule.interval = val.parse::<i64>().ok().filter(|i| (1..=MAX_INTERVAL).contains(i))?,
            "COUNT" => rule.count = Some(val.parse().ok()?),
            "UNTIL" => {
                let p = Prop { name: "UNTIL".into(), params: HashMap::new(), value: val.into() };
                let mut until = parse_when(&p, zones)?;
                // A floating or date-only UNTIL is in the zone of the start.
                if matches!(until.zone, Zone::Local) {
                    until.zone = start.zone.clone();
                }
                let whole_day = until.date_only;
                if whole_day {
                    until.wall += Duration::days(1);
                }
                rule.until = Some((until, whole_day));
            }
            "BYDAY" => rule.by_day = by_day(val)?,
            "BYMONTHDAY" => rule.by_month_day = numbers(val)?,
            "BYMONTH" => rule.by_month = numbers(val)?,
            "BYSETPOS" => rule.by_set_pos = numbers(val)?,
            "WKST" => {}
            _ => return None,
        }
    }
    let ok = match rule.freq.as_str() {
        "DAILY" | "MONTHLY" => true,
        // numbered weekdays only make sense within a month
        "WEEKLY" => rule.by_day.iter().all(|(n, _)| *n == 0),
        "YEARLY" => rule.by_day.is_empty() || !rule.by_month.is_empty(),
        _ => false,
    };
    ok.then_some(rule)
}

/// The n-th (1-based; negative from the end) `wd` of a month.
fn nth_weekday(year: i32, month: u32, n: i32, wd: Weekday) -> Option<NaiveDate> {
    let matching = all_weekdays(year, month, wd);
    let i = if n > 0 { n - 1 } else { matching.len() as i32 + n };
    matching.get(usize::try_from(i).ok()?).copied()
}

fn all_weekdays(year: i32, month: u32, wd: Weekday) -> Vec<NaiveDate> {
    let (Some(first), Some(last)) = (NaiveDate::from_ymd_opt(year, month, 1), month_day(year, month, -1)) else { return Vec::new() };
    (0..last.day()).map(|i| first + Duration::days(i as i64)).filter(|d| d.weekday() == wd).collect()
}

/// Day `day` of a month (negative: from the end, -1 = the last); `None` when
/// the month has no such day.
fn month_day(year: i32, month: u32, day: i32) -> Option<NaiveDate> {
    if !(1..=31).contains(&day.unsigned_abs()) {
        return None;
    }
    if day > 0 {
        return NaiveDate::from_ymd_opt(year, month, day as u32);
    }
    let next = if month == 12 { NaiveDate::from_ymd_opt(year + 1, 1, 1)? } else { NaiveDate::from_ymd_opt(year, month + 1, 1)? };
    Some(next + Duration::days(day as i64)).filter(|d| d.month() == month)
}

/// Days of one month a MONTHLY / YEARLY rule picks (before BYSETPOS).
fn month_days(rule: &Rule, year: i32, month: u32, default_day: u32) -> Vec<NaiveDate> {
    let mut days: Vec<NaiveDate> = if !rule.by_day.is_empty() {
        rule.by_day
            .iter()
            .flat_map(|&(n, wd)| if n == 0 { all_weekdays(year, month, wd) } else { nth_weekday(year, month, n, wd).into_iter().collect() })
            .filter(|d| rule.by_month_day.is_empty() || rule.by_month_day.iter().any(|&md| month_day(year, month, md) == Some(*d)))
            .collect()
    } else if !rule.by_month_day.is_empty() {
        rule.by_month_day.iter().filter_map(|&d| month_day(year, month, d)).collect()
    } else {
        NaiveDate::from_ymd_opt(year, month, default_day).into_iter().collect()
    };
    days.sort();
    days.dedup();
    days
}

/// BYSETPOS: the 1-based (negative from the end) picks out of a period's days.
fn set_pos(days: Vec<NaiveDate>, pos: &[i32]) -> Vec<NaiveDate> {
    if pos.is_empty() {
        return days;
    }
    let mut out: Vec<NaiveDate> = pos
        .iter()
        .filter_map(|&p| {
            let i = if p > 0 { p - 1 } else { days.len() as i32 + p };
            days.get(usize::try_from(i).ok()?).copied()
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Whole periods of the rule between `first` and `from`, to start expanding
/// shortly before the window (only when no COUNT makes earlier ones matter).
fn periods_before(rule: &Rule, first: NaiveDate, from_ms: i64) -> i64 {
    let Some(from) = DateTime::from_timestamp_millis(from_ms).map(|d| d.naive_utc().date()) else { return 0 };
    let months = |a: NaiveDate, b: NaiveDate| (b.year() - a.year()) as i64 * 12 + b.month0() as i64 - a.month0() as i64;
    let span = match rule.freq.as_str() {
        "DAILY" => (from - first).num_days(),
        "WEEKLY" => (from - first).num_weeks(),
        "MONTHLY" => months(first, from),
        _ => months(first, from) / 12,
    };
    (span / rule.interval - 2).max(0)
}

/// Wall times of a rule's occurrences, in order, up to `to_ms`: DTSTART first
/// (always an instance), then what the rule generates.
fn occurrences(rule: &Rule, start: &When, from_ms: i64, to_ms: i64) -> Vec<NaiveDateTime> {
    let time = start.wall.time();
    let first = start.wall.date();
    let mut out = Vec::new();
    let mut count = 0usize;
    // false = the series is over
    // `generated`: made by the rule (not DTSTART), so dropped if its wall time doesn't exist.
    let mut push = |wall: NaiveDateTime, generated: bool, out: &mut Vec<NaiveDateTime>| -> bool {
        let at = start.with_wall(wall);
        if generated && !at.exists() {
            return true; // a DST gap: no such instance, and it isn't counted
        }
        if let Some((until, exclusive)) = &rule.until {
            if let (Some(ms), Some(end)) = (at.ms(), until.ms()) {
                if if *exclusive { ms >= end } else { ms > end } {
                    return false;
                }
            }
        }
        if rule.count.is_some_and(|c| count >= c) {
            return false;
        }
        let Some(ms) = at.ms() else { return true };
        if ms >= to_ms {
            return false;
        }
        count += 1;
        out.push(wall);
        true
    };
    if !push(start.wall, false, &mut out) {
        return out;
    }
    let skip = if rule.count.is_none() { periods_before(rule, first, from_ms) } else { 0 };
    let in_months = |d: &NaiveDate| rule.by_month.is_empty() || rule.by_month.contains(&d.month());
    for step in skip..skip + MAX_STEPS as i64 {
        let n = step * rule.interval;
        let days: Vec<NaiveDate> = match rule.freq.as_str() {
            "DAILY" => {
                let Some(d) = first.checked_add_signed(Duration::days(n)) else { break };
                let wd_ok = rule.by_day.is_empty() || rule.by_day.iter().any(|(_, wd)| *wd == d.weekday());
                let md_ok = rule.by_month_day.is_empty() || rule.by_month_day.iter().any(|&md| month_day(d.year(), d.month(), md) == Some(d));
                if wd_ok && md_ok && in_months(&d) { vec![d] } else { Vec::new() }
            }
            "WEEKLY" => {
                let monday = first - Duration::days(first.weekday().num_days_from_monday() as i64);
                let (Some(week), Some(same)) = (monday.checked_add_signed(Duration::weeks(n)), first.checked_add_signed(Duration::weeks(n))) else { break };
                let mut days: Vec<NaiveDate> = if rule.by_day.is_empty() {
                    vec![same]
                } else {
                    rule.by_day.iter().map(|(_, wd)| week + Duration::days(wd.num_days_from_monday() as i64)).collect()
                };
                days.retain(in_months);
                days.sort();
                set_pos(days, &rule.by_set_pos)
            }
            "MONTHLY" => {
                let months = first.month0() as i64 + n;
                let (y, m) = (first.year() + months.div_euclid(12) as i32, months.rem_euclid(12) as u32 + 1);
                if !rule.by_month.is_empty() && !rule.by_month.contains(&m) {
                    Vec::new()
                } else {
                    set_pos(month_days(rule, y, m, first.day()), &rule.by_set_pos)
                }
            }
            _ => {
                let y = first.year() + n as i32;
                // BYMONTHDAY alone means those days of every month.
                let months = match (rule.by_month.is_empty(), rule.by_month_day.is_empty()) {
                    (false, _) => rule.by_month.clone(),
                    (true, false) => (1..=12).collect(),
                    (true, true) => vec![first.month()],
                };
                let mut days: Vec<NaiveDate> = months.iter().flat_map(|&m| month_days(rule, y, m, first.day())).collect();
                days.sort();
                set_pos(days, &rule.by_set_pos)
            }
        };
        for d in days {
            let wall = d.and_time(time);
            if wall <= start.wall {
                continue;
            }
            if !push(wall, true, &mut out) {
                return out;
            }
        }
    }
    out
}

/// A STANDARD / DAYLIGHT part. Its RRULE gives the yearly onset (usually
/// FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU) and when the rule ended (UNTIL, COUNT:
/// zones carry their past rules too).
fn tz_part(start: NaiveDateTime, from: i32, to: i32, rule: Option<&str>) -> TzPart {
    let mut part = TzPart { start, from, to, yearly: None, until: None, last_year: None };
    let Some(rule) = rule else { return part };
    let get = |k: &str| rule.split(';').find_map(|kv| kv.split_once('=').filter(|(key, _)| key.trim().eq_ignore_ascii_case(k)).map(|(_, v)| v.trim()));
    let month = get("BYMONTH").and_then(|m| m.parse::<u32>().ok()).filter(|m| (1..=12).contains(m));
    let day = get("BYDAY").and_then(by_day).and_then(|d| d.first().copied()).filter(|(n, _)| *n != 0);
    // Written some other way (BYMONTHDAY…): the same week of the same month
    // as the first onset, the last week staying the last.
    let derived = || {
        let d = start.date();
        let last = month_day(d.year(), d.month(), -1).is_some_and(|l| d.day() + 7 > l.day());
        (d.month(), if last { -1 } else { ((d.day() - 1) / 7 + 1) as i32 }, d.weekday())
    };
    part.yearly = Some(month.zip(day).map(|(m, (n, wd))| (m, n, wd)).unwrap_or_else(derived));
    part.until = get("UNTIL").and_then(|u| {
        NaiveDateTime::parse_from_str(u.trim_end_matches(['Z', 'z']), "%Y%m%dT%H%M%S")
            .ok()
            .or_else(|| NaiveDate::parse_from_str(u.get(..8)?, "%Y%m%d").ok().map(|d| d.and_time(NaiveTime::MIN) + Duration::days(1)))
    });
    part.last_year = get("COUNT").and_then(|c| c.parse::<i32>().ok()).map(|c| start.year().saturating_add(c.max(1) - 1));
    part
}

/// Line value after the first ':'.
fn value(line: &str) -> Option<&str> {
    line.split_once(':').map(|(_, v)| v.trim())
}

/// Events overlapping [from, to) (unix ms), recurrences expanded.
pub fn parse(text: &str, from: i64, to: i64) -> Vec<Event> {
    // Unfold: a line starting with a space or tab continues the previous one.
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.trim_end_matches('\r');
        match raw.strip_prefix([' ', '\t']) {
            Some(rest) if !lines.is_empty() => lines.last_mut().unwrap().push_str(rest),
            _ => lines.push(raw.to_string()),
        }
    }
    // Components: time zones with their STANDARD/DAYLIGHT parts, and events.
    let mut zones: Zones = HashMap::new();
    let mut events: Vec<Vec<String>> = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut tz_id: Option<String> = None;
    let mut tz = Tz::default();
    // DTSTART, TZOFFSETFROM, TZOFFSETTO and RRULE of the STANDARD/DAYLIGHT being read.
    let mut part: Option<(Option<NaiveDateTime>, Option<i32>, Option<i32>, Option<String>)> = None;
    let mut current: Vec<String> = Vec::new();
    for line in lines {
        let upper = line.to_ascii_uppercase();
        if let Some(name) = upper.strip_prefix("BEGIN:") {
            let name = name.trim().to_string();
            match name.as_str() {
                "VEVENT" => current.clear(),
                "VTIMEZONE" => {
                    tz_id = None;
                    tz = Tz::default();
                }
                "STANDARD" | "DAYLIGHT" => part = Some((None, None, None, None)),
                _ => {}
            }
            stack.push(name);
            continue;
        }
        if let Some(name) = upper.strip_prefix("END:") {
            match name.trim() {
                "VEVENT" => events.push(std::mem::take(&mut current)),
                "STANDARD" | "DAYLIGHT" => {
                    if let Some((Some(start), from_off, Some(to_off), rule)) = part.take() {
                        tz.parts.push(tz_part(start, from_off.unwrap_or(to_off), to_off, rule.as_deref()));
                    }
                }
                "VTIMEZONE" => {
                    if let Some(id) = tz_id.take() {
                        if !tz.parts.is_empty() {
                            zones.insert(id, Rc::new(std::mem::take(&mut tz)));
                        }
                    }
                }
                _ => {}
            }
            stack.pop();
            continue;
        }
        match stack.last().map(String::as_str) {
            Some("VEVENT") => current.push(line),
            Some("VTIMEZONE") if upper.starts_with("TZID") => tz_id = value(&line).map(str::to_string),
            Some("STANDARD" | "DAYLIGHT") => {
                let Some(p) = part.as_mut() else { continue };
                if upper.starts_with("DTSTART") {
                    p.0 = value(&line).and_then(|v| NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok());
                } else if upper.starts_with("TZOFFSETFROM") {
                    p.1 = value(&line).and_then(offset);
                } else if upper.starts_with("TZOFFSETTO") {
                    p.2 = value(&line).and_then(offset);
                } else if upper.starts_with("RRULE") {
                    p.3 = value(&line).map(str::to_string);
                }
            }
            _ => {}
        }
    }

    struct Raw {
        uid: String,
        title: String,
        start: When,
        length: Duration,
        location: String,
        link: Option<String>,
        rule: Option<String>,
        exdates: Vec<i64>,
        exdays: Vec<NaiveDate>,
        recurrence_id: Option<i64>,
        cancelled: bool,
    }
    let mut raws: Vec<Raw> = Vec::new();
    for lines in events {
        let ps = props(&lines);
        let get = |name: &str| ps.iter().find(|p| p.name == name);
        let cancelled = get("STATUS").is_some_and(|p| p.value.trim().eq_ignore_ascii_case("CANCELLED"));
        let recurrence_id = get("RECURRENCE-ID").and_then(|p| parse_when(p, &zones)).and_then(|w| w.ms());
        // A whole cancelled event is just gone; a cancelled instance still has to hide its occurrence.
        if cancelled && recurrence_id.is_none() {
            continue;
        }
        let Some(start) = get("DTSTART").and_then(|p| parse_when(p, &zones)) else { continue };
        let length = match (get("DTEND").and_then(|p| parse_when(p, &zones)), get("DURATION")) {
            (Some(end), _) => Duration::milliseconds(end.ms().unwrap_or(0) - start.ms().unwrap_or(0)),
            (None, Some(d)) => parse_duration(d.value.trim()).unwrap_or_default(),
            (None, None) if start.date_only => Duration::days(1),
            _ => Duration::zero(),
        };
        let text = |name: &str| get(name).map(|p| unescape(&p.value)).unwrap_or_default();
        let (location, description) = (text("LOCATION"), text("DESCRIPTION"));
        // The meeting's own link fields first: URL, CONFERENCE and the video
        // fields calendars add as X- properties (they beat a link merely
        // mentioned in the description); then LOCATION, DESCRIPTION, and any
        // other property (an HTML description too: call_link undoes &amp;).
        let video_field = |p: &&Prop| p.name.starts_with("X-") && ["CONF", "VIDEO", "MEET", "CALL"].iter().any(|k| p.name.contains(k));
        let link = ["URL", "CONFERENCE"]
            .iter()
            .find_map(|name| get(name).and_then(|p| call_link(&p.value)))
            .or_else(|| ps.iter().filter(video_field).find_map(|p| call_link(&unescape(&p.value))))
            .or_else(|| call_link(&location))
            .or_else(|| call_link(&description))
            .or_else(|| ps.iter().find_map(|p| call_link(&unescape(&p.value))));
        let (mut exdates, mut exdays) = (Vec::new(), Vec::new());
        for p in ps.iter().filter(|p| p.name == "EXDATE") {
            for v in p.value.split(',') {
                let one = Prop { name: "EXDATE".into(), params: p.params.clone(), value: v.into() };
                match parse_when(&one, &zones) {
                    Some(w) if w.date_only => exdays.push(w.wall.date()),
                    Some(w) => exdates.extend(w.ms()),
                    None => {}
                }
            }
        }
        raws.push(Raw {
            uid: text("UID"),
            title: text("SUMMARY"),
            start,
            length,
            location,
            link,
            rule: get("RRULE").map(|p| p.value.trim().to_string()),
            exdates,
            exdays,
            recurrence_id,
            cancelled,
        });
    }

    // Instances moved, changed or cancelled on their own replace the rule's occurrence.
    let moved: Vec<(String, i64)> = raws.iter().filter_map(|r| Some((r.uid.clone(), r.recurrence_id?))).collect();
    let mut out = Vec::new();
    for r in raws.iter().filter(|r| !r.cancelled) {
        let starts: Vec<When> = match (&r.rule, r.recurrence_id) {
            (Some(rule), None) => match parse_rule(rule, &zones, &r.start) {
                Some(rule) => occurrences(&rule, &r.start, from, to).into_iter().map(|w| r.start.with_wall(w)).collect(),
                None => vec![r.start.clone()],
            },
            _ => vec![r.start.clone()],
        };
        for s in starts {
            let Some(start) = s.ms() else { continue };
            if r.recurrence_id.is_none() && r.rule.is_some() {
                let excluded = r.exdates.contains(&start) || r.exdays.contains(&s.wall.date());
                if excluded || moved.iter().any(|(uid, at)| *uid == r.uid && *at == start) {
                    continue;
                }
            }
            let end = start + r.length.num_milliseconds().max(0);
            if end <= from && end != start || start >= to || (end == start && start < from) {
                continue;
            }
            out.push(Event {
                id: format!("{}@{}", r.uid, r.recurrence_id.unwrap_or(start)),
                title: if r.title.trim().is_empty() { "Без названия".into() } else { r.title.trim().to_string() },
                start,
                end,
                all_day: s.date_only,
                location: r.location.trim().to_string(),
                link: r.link.clone(),
            });
        }
    }
    out.sort_by_key(|e| (e.start, e.end));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Local midnight `days` from today, unix ms.
pub fn day_start(days: i64) -> i64 {
    let date = Local::now().date_naive() + Duration::days(days);
    Local.from_local_datetime(&date.and_time(NaiveTime::MIN)).earliest().map_or(0, |d: DateTime<Local>| d.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cal(body: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Moscow\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:+0300\r\nTZOFFSETTO:+0300\r\n\
             DTSTART:19700101T000000\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n{body}END:VCALENDAR\r\n"
        )
    }
    fn utc(s: &str) -> i64 {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap().and_utc().timestamp_millis()
    }
    const ALL: (i64, i64) = (0, i64::MAX / 2);

    #[test]
    fn simple_event_in_a_named_zone() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:a\r\nSUMMARY:Созвон\\, команда\r\nDTSTART;TZID=Europe/Moscow:20261021T110000\r\n\
             DTEND;TZID=Europe/Moscow:20261021T113000\r\nLOCATION:https://telemost.yandex.ru/j/123\r\nEND:VEVENT\r\n",
        );
        let ev = parse(&text, ALL.0, ALL.1);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].title, "Созвон, команда");
        assert_eq!((ev[0].start, ev[0].end), (utc("2026-10-21 08:00"), utc("2026-10-21 08:30")));
        assert_eq!(ev[0].link.as_deref(), Some("https://telemost.yandex.ru/j/123"));
        assert!(!ev[0].all_day);
    }

    #[test]
    fn folded_lines_utc_times_and_description_links() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:b\r\nSUMMARY:Долгое\r\n  название\r\nDTSTART:20261021T090000Z\r\nDURATION:PT1H30M\r\n\
             DESCRIPTION:Ссылка: https://us02web.zoom.us/j/999?pwd=x.\\nПароль 1\r\nEND:VEVENT\r\n",
        );
        let ev = parse(&text, ALL.0, ALL.1);
        assert_eq!(ev[0].title, "Долгое название");
        assert_eq!(ev[0].end - ev[0].start, 90 * 60_000);
        assert_eq!(ev[0].link.as_deref(), Some("https://us02web.zoom.us/j/999?pwd=x"));
    }

    #[test]
    fn ktalk_links_wherever_they_are() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:k1\r\nSUMMARY:Дейли\r\nDTSTART:20261026T073000Z\r\nX-MAILRU-CONFERENCE:https://ktstech.ktalk.ru/w7ek90r2wsbc\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:k2\r\nSUMMARY:1to1\r\nDTSTART:20261026T133000Z\r\nDESCRIPTION:https://ktstech.ktalk.ru/ba8n84sf6sxj\r\nEND:VEVENT\r\n",
        );
        let links: Vec<Option<String>> = parse(&text, ALL.0, ALL.1).into_iter().map(|e| e.link).collect();
        assert_eq!(
            links,
            vec![Some("https://ktstech.ktalk.ru/w7ek90r2wsbc".into()), Some("https://ktstech.ktalk.ru/ba8n84sf6sxj".into())]
        );
    }

    #[test]
    fn call_links_by_host() {
        assert!(is_call("https://ktstech.ktalk.ru/w7ek90r2wsbc"));
        assert!(is_call("https://us02web.zoom.us/j/1"));
        assert!(is_call("https://vk.com/call/join/x"));
        assert!(!is_call("https://vk.com/feed"));
        assert!(!is_call("https://notktalk.ru/x"));
        assert!(!is_call("https://ktalk.ru.example.com/x"));
        assert!(!is_call("https://www.google.com/url?q=https://team.ktalk.ru/x"));
        // A video field beats a link only mentioned in the description; one
        // found only in the HTML description comes out without &amp;.
        let text = cal(
            "BEGIN:VEVENT\r\nUID:v\r\nSUMMARY:x\r\nDTSTART:20261026T073000Z\r\nDESCRIPTION:старая https://zoom.us/j/old\r\n\
             X-ALT-DESC;FMTTYPE=text/html:<a href=\"https://zoom.us/j/1?a=1&amp;b=2\">\r\nX-VIDEO-LINK:https://team.ktalk.ru/new\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:h\r\nSUMMARY:x\r\nDTSTART:20261026T083000Z\r\nX-ALT-DESC;FMTTYPE=text/html:<a href=\"https://zoom.us/j/1?a=1&amp;b=2\">\r\nEND:VEVENT\r\n",
        );
        let links: Vec<Option<String>> = parse(&text, ALL.0, ALL.1).into_iter().map(|e| e.link).collect();
        assert_eq!(links, vec![Some("https://team.ktalk.ru/new".into()), Some("https://zoom.us/j/1?a=1&b=2".into())]);
    }

    #[test]
    fn all_day_and_cancelled() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:c\r\nSUMMARY:Отпуск\r\nDTSTART;VALUE=DATE:20261021\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:d\r\nSTATUS:CANCELLED\r\nSUMMARY:Нет\r\nDTSTART:20261021T090000Z\r\nEND:VEVENT\r\n",
        );
        let ev = parse(&text, ALL.0, ALL.1);
        assert_eq!(ev.len(), 1);
        assert!(ev[0].all_day);
        assert_eq!(ev[0].end - ev[0].start, 86_400_000);
    }

    #[test]
    fn weekly_rule_with_exception_and_moved_instance() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:w\r\nSUMMARY:Планёрка\r\nDTSTART;TZID=Europe/Moscow:20261019T100000\r\nDURATION:PT30M\r\n\
             RRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=6\r\nEXDATE;TZID=Europe/Moscow:20261021T100000\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:w\r\nRECURRENCE-ID;TZID=Europe/Moscow:20261026T100000\r\nSUMMARY:Планёрка (перенос)\r\n\
             DTSTART;TZID=Europe/Moscow:20261026T150000\r\nDURATION:PT30M\r\nEND:VEVENT\r\n",
        );
        let ev = parse(&text, ALL.0, ALL.1);
        let starts: Vec<i64> = ev.iter().map(|e| e.start).collect();
        // Mo 19, (We 21 excluded), Mo 26 moved to 15:00, We 28, Mo 2, We 4 — six by COUNT.
        assert_eq!(
            starts,
            vec![utc("2026-10-19 07:00"), utc("2026-10-26 12:00"), utc("2026-10-28 07:00"), utc("2026-11-02 07:00"), utc("2026-11-04 07:00")]
        );
        assert_eq!(ev[1].title, "Планёрка (перенос)");
    }

    #[test]
    fn monthly_by_nth_weekday_and_window() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:m\r\nSUMMARY:Ретро\r\nDTSTART:20260101T120000Z\r\nDTEND:20260101T130000Z\r\n\
             RRULE:FREQ=MONTHLY;BYDAY=-1FR\r\nEND:VEVENT\r\n",
        );
        let ev = parse(&text, utc("2026-10-01 00:00"), utc("2026-12-01 00:00"));
        let starts: Vec<i64> = ev.iter().map(|e| e.start).collect();
        assert_eq!(starts, vec![utc("2026-10-30 12:00"), utc("2026-11-27 12:00")]);
    }

    #[test]
    fn daily_until_and_interval() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:d2\r\nSUMMARY:Стендап\r\nDTSTART:20261020T070000Z\r\nDURATION:PT15M\r\n\
             RRULE:FREQ=DAILY;INTERVAL=2;UNTIL=20261026T070000Z\r\nEND:VEVENT\r\n",
        );
        let n = parse(&text, ALL.0, ALL.1).len();
        assert_eq!(n, 4); // 20, 22, 24, 26
    }

    fn starts(text: &str, from: i64, to: i64) -> Vec<i64> {
        parse(text, from, to).iter().map(|e| e.start).collect()
    }
    fn event(rule: &str, start: &str) -> String {
        cal(&format!(
            "BEGIN:VEVENT\r\nUID:r\r\nSUMMARY:x\r\nDTSTART:{start}\r\nDURATION:PT30M\r\nRRULE:{rule}\r\nEND:VEVENT\r\n"
        ))
    }

    #[test]
    fn daylight_saving_in_a_described_zone() {
        let text = "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n\
             BEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nDTSTART:19810329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\n\
             BEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nDTSTART:19961027T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
             END:VTIMEZONE\r\n\
             BEGIN:VEVENT\r\nUID:z\r\nSUMMARY:x\r\nDTSTART;TZID=Europe/Berlin:20261023T100000\r\nDURATION:PT1H\r\n\
             RRULE:FREQ=DAILY;COUNT=4\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        // Summer time ends on Sunday 25 October 2026.
        assert_eq!(
            starts(text, ALL.0, ALL.1),
            vec![utc("2026-10-23 08:00"), utc("2026-10-24 08:00"), utc("2026-10-25 09:00"), utc("2026-10-26 09:00")]
        );
        // And starts on 28 March.
        let spring = text.replace("20261023T100000", "20260327T100000");
        assert_eq!(starts(&spring, ALL.0, ALL.1)[1..3], [utc("2026-03-28 09:00"), utc("2026-03-29 08:00")]);
    }

    #[test]
    fn daily_by_weekdays() {
        // Fri 23 Oct: then Mon, Tue — the weekend skipped.
        let text = event("FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR;COUNT=3", "20261023T070000Z");
        assert_eq!(starts(&text, ALL.0, ALL.1), vec![utc("2026-10-23 07:00"), utc("2026-10-26 07:00"), utc("2026-10-27 07:00")]);
    }

    #[test]
    fn monthly_last_weekday_by_set_pos() {
        let text = event("FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1", "20260130T070000Z");
        // October 2026 ends on a Saturday, so its last weekday is Fri 30; November's is Mon 30.
        assert_eq!(starts(&text, utc("2026-10-01 00:00"), utc("2026-12-01 00:00")), vec![utc("2026-10-30 07:00"), utc("2026-11-30 07:00")]);
    }

    #[test]
    fn yearly_by_month_and_day() {
        let text = event("FREQ=YEARLY;BYMONTH=3,10;BYDAY=-1SU", "20250330T070000Z");
        assert_eq!(starts(&text, utc("2026-01-01 00:00"), utc("2027-01-01 00:00")), vec![utc("2026-03-29 07:00"), utc("2026-10-25 07:00")]);
    }

    #[test]
    fn cancelled_instance_hides_its_occurrence() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:c\r\nSUMMARY:x\r\nDTSTART:20261019T070000Z\r\nDURATION:PT30M\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:c\r\nRECURRENCE-ID:20261020T070000Z\r\nSTATUS:CANCELLED\r\nSUMMARY:x\r\nDTSTART:20261020T070000Z\r\nEND:VEVENT\r\n",
        );
        assert_eq!(starts(&text, ALL.0, ALL.1), vec![utc("2026-10-19 07:00"), utc("2026-10-21 07:00")]);
    }

    #[test]
    fn dtstart_off_the_rule_counts_and_date_until_is_inclusive() {
        // Sunday start, Monday rule: the start itself is the first of two.
        let text = event("FREQ=WEEKLY;BYDAY=MO;COUNT=2", "20261018T070000Z");
        assert_eq!(starts(&text, ALL.0, ALL.1), vec![utc("2026-10-18 07:00"), utc("2026-10-19 07:00")]);
        // A date-only UNTIL takes its whole day; a trailing ';' is fine.
        let text = event("FREQ=DAILY;UNTIL=20261021;", "20261019T150000Z");
        assert_eq!(starts(&text, ALL.0, ALL.1).len(), 3);
    }

    #[test]
    fn unsupported_rule_shows_only_the_start() {
        let text = event("FREQ=MONTHLY;BYWEEKNO=20", "20261019T070000Z");
        assert_eq!(starts(&text, ALL.0, ALL.1), vec![utc("2026-10-19 07:00")]);
        let text = event("FREQ=HOURLY", "20261019T070000Z");
        assert_eq!(starts(&text, ALL.0, ALL.1).len(), 1);
    }

    #[test]
    fn old_daily_series_still_reaches_today() {
        let text = event("FREQ=DAILY", "20100104T070000Z");
        assert_eq!(starts(&text, utc("2026-10-21 00:00"), utc("2026-10-23 00:00")), vec![utc("2026-10-21 07:00"), utc("2026-10-22 07:00")]);
    }

    #[test]
    fn past_zone_rules_end_with_their_until() {
        // New York as Apple writes it: the 1987–2006 rules, then today's.
        let text = "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:America/New_York\r\n\
             BEGIN:DAYLIGHT\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\nDTSTART:19870405T020000\r\nRRULE:FREQ=YEARLY;UNTIL=20060402T070000Z;BYMONTH=4;BYDAY=1SU\r\nEND:DAYLIGHT\r\n\
             BEGIN:STANDARD\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nDTSTART:19671029T020000\r\nRRULE:FREQ=YEARLY;UNTIL=20061029T060000Z;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
             BEGIN:DAYLIGHT\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\nDTSTART:20070311T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\r\nEND:DAYLIGHT\r\n\
             BEGIN:STANDARD\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nDTSTART:20071104T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\nEND:STANDARD\r\n\
             END:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:ny\r\nSUMMARY:x\r\nDTSTART;TZID=America/New_York:20261028T100000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert_eq!(starts(text, ALL.0, ALL.1), vec![utc("2026-10-28 14:00")]);
        let winter = text.replace("20261028T100000", "20261110T100000");
        assert_eq!(starts(&winter, ALL.0, ALL.1), vec![utc("2026-11-10 15:00")]);
    }

    #[test]
    fn a_zone_whose_rules_all_ended_keeps_the_last_offset() {
        // Moscow as written around 2012: summer time until 2011, then nothing.
        let text = "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Moscow\r\n\
             BEGIN:STANDARD\r\nTZOFFSETFROM:+023017\r\nTZOFFSETTO:+023017\r\nDTSTART:18800101T000000\r\nEND:STANDARD\r\n\
             BEGIN:STANDARD\r\nTZOFFSETFROM:+0400\r\nTZOFFSETTO:+0300\r\nDTSTART:19961027T030000\r\nRRULE:FREQ=YEARLY;UNTIL=20101030T230000Z;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
             BEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0300\r\nTZOFFSETTO:+0400\r\nDTSTART:19930328T020000\r\nRRULE:FREQ=YEARLY;UNTIL=20110326T230000Z;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\n\
             END:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:m\r\nSUMMARY:x\r\nDTSTART;TZID=Europe/Moscow:20120604T100000\r\nRRULE:FREQ=YEARLY;COUNT=3\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert_eq!(starts(text, ALL.0, ALL.1), vec![utc("2012-06-04 06:00"), utc("2013-06-04 06:00"), utc("2014-06-04 06:00")]);
    }

    #[test]
    fn times_skipped_by_dst_in_a_described_zone() {
        let text = "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Berlin\r\n\
             BEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nDTSTART:19810329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\n\
             BEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nDTSTART:19961027T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\n\
             END:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:g\r\nSUMMARY:x\r\nDTSTART;TZID=Europe/Berlin:20260328T023000\r\n\
             RRULE:FREQ=DAILY;COUNT=3\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        // 29 March 02:30 doesn't exist there: that instance is dropped and not counted.
        assert_eq!(starts(text, ALL.0, ALL.1), vec![utc("2026-03-28 01:30"), utc("2026-03-30 00:30"), utc("2026-03-31 00:30")]);
        // A start in the gap is read with the offset from before it.
        let single = text.replace("20260328T023000", "20260329T023000").replace("RRULE:FREQ=DAILY;COUNT=3\r\n", "");
        assert_eq!(starts(&single, ALL.0, ALL.1), vec![utc("2026-03-29 01:30")]);
    }

    #[test]
    fn month_days_stay_in_their_month() {
        let text = event("FREQ=MONTHLY;BYMONTHDAY=-31;COUNT=3", "20260131T070000Z");
        // Only months with 31 days have a 31st-from-the-end.
        assert_eq!(starts(&text, ALL.0, ALL.1), vec![utc("2026-01-31 07:00"), utc("2026-03-01 07:00"), utc("2026-05-01 07:00")]);
        let text = event("FREQ=YEARLY;BYMONTHDAY=1;COUNT=3", "20260101T070000Z");
        assert_eq!(starts(&text, ALL.0, ALL.1), vec![utc("2026-01-01 07:00"), utc("2026-02-01 07:00"), utc("2026-03-01 07:00")]);
    }

    #[test]
    fn garbage_rules_dont_panic() {
        for rule in ["FREQ=WEEKLY;BYDAY=éX", "FREQ=DAILY;INTERVAL=200000000", "FREQ=WEEKLY;INTERVAL=30000000", "FREQ=MONTHLY;BYMONTHDAY=0,99,-2147483648"] {
            let text = event(rule, "20261019T070000Z");
            assert!(!starts(&text, ALL.0, ALL.1).is_empty(), "{rule}");
        }
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("PT1H30M"), Some(Duration::minutes(90)));
        assert_eq!(parse_duration("P1DT2H"), Some(Duration::hours(26)));
        assert_eq!(parse_duration("-PT15M"), Some(Duration::minutes(-15)));
        assert_eq!(parse_duration("P2W"), Some(Duration::days(14)));
        assert_eq!(parse_duration("1H"), None);
    }
}
